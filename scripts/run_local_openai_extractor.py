#!/usr/bin/env python3
"""Shell-free stdin/stdout adapter for a loopback OpenAI-compatible extractor.

The product extraction worker deliberately accepts an argv provider contract.
This adapter lets that contract target local servers such as oMLX without an
API key: it reads the versioned Anamnesis prompt from stdin and writes only the
assistant's JSON object to stdout.
"""

from __future__ import annotations

import argparse
import ipaddress
import json
import sys
import urllib.error
import urllib.parse
import urllib.request


class NoRedirectHandler(urllib.request.HTTPRedirectHandler):
    """Keep a loopback request from being redirected off-host."""

    def redirect_request(self, req, fp, code, msg, headers, newurl):  # noqa: N802
        return None


def local_chat_url(base_url: str) -> str:
    parsed = urllib.parse.urlparse(base_url.rstrip("/"))
    if parsed.scheme != "http" or not parsed.hostname:
        raise ValueError("base URL must use HTTP on a loopback host")
    try:
        local = ipaddress.ip_address(parsed.hostname).is_loopback
    except ValueError:
        local = parsed.hostname.lower() == "localhost"
    if not local or parsed.username or parsed.password or parsed.query or parsed.fragment:
        raise ValueError("base URL must use HTTP on a loopback host")
    path = parsed.path.rstrip("/")
    if path.endswith("/v1"):
        path = f"{path}/chat/completions"
    else:
        path = f"{path}/v1/chat/completions"
    return urllib.parse.urlunparse(parsed._replace(path=path))


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--base-url", default="http://127.0.0.1:8000")
    parser.add_argument("--model", required=True)
    parser.add_argument("--timeout-secs", type=int, default=600)
    args = parser.parse_args()
    if args.timeout_secs <= 0:
        parser.error("--timeout-secs must be positive")

    prompt = sys.stdin.read()
    if not prompt:
        raise RuntimeError("extractor prompt is empty")
    body = json.dumps(
        {
            "model": args.model,
            "messages": [{"role": "user", "content": prompt}],
            "stream": False,
            "temperature": 0,
            "top_p": 1,
            "top_k": 20,
            "presence_penalty": 0,
            "seed": 42,
            "max_tokens": 8192,
            "chat_template_kwargs": {"enable_thinking": False},
            "response_format": {"type": "json_object"},
        }
    ).encode()
    request = urllib.request.Request(
        local_chat_url(args.base_url),
        data=body,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    opener = urllib.request.build_opener(
        urllib.request.ProxyHandler({}),
        NoRedirectHandler(),
    )
    try:
        with opener.open(request, timeout=args.timeout_secs) as response:
            payload = json.load(response)
    except (urllib.error.URLError, TimeoutError) as error:
        raise RuntimeError("local extraction provider request failed") from error
    choices = payload.get("choices")
    if not isinstance(choices, list) or not choices:
        raise RuntimeError("local extraction provider returned no choices")
    message = choices[0].get("message")
    content = message.get("content") if isinstance(message, dict) else None
    if not isinstance(content, str) or not content.strip():
        raise RuntimeError("local extraction provider returned empty content")
    sys.stdout.write(content)
    if not content.endswith("\n"):
        sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
