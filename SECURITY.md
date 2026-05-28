# Security Policy

## Supported Versions

| Version | Supported |
|:--------|:----------|
| 0.4.x   | Yes       |
| < 0.4   | No        |

## Reporting a Vulnerability

If you discover a security vulnerability, please report it responsibly.

**Do NOT open a public GitHub issue for security vulnerabilities.**

Instead, use [GitHub's private vulnerability reporting](https://github.com/INONONO66/anamnesis/security/advisories/new) to submit your report.

### What to include

- Description of the vulnerability
- Steps to reproduce
- Potential impact
- Suggested fix (if any)

### Response timeline

- **Acknowledgment**: within 48 hours
- **Initial assessment**: within 1 week
- **Fix or mitigation**: depends on severity, but we aim for prompt resolution

## Scope

Anamnesis is a library with no network exposure from the core engine. The default storage adapter uses bundled SQLite, and the optional `embed` feature can download a local embedding model on first use. The primary attack surface is malicious input to public API methods, storage files opened by consumers, and optional embedding-provider model downloads.

## Disclosure Policy

We follow coordinated disclosure. Once a fix is released, we will publish a security advisory on GitHub.
