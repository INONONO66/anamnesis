#!/usr/bin/env bash
# Download LongMemEval dataset from HuggingFace
# Source: https://github.com/xiaowu0162/LongMemEval
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DATA_DIR="$SCRIPT_DIR/data"
mkdir -p "$DATA_DIR"

echo "Downloading LongMemEval dataset..."
echo "Source: https://huggingface.co/datasets/xiaowu0162/LongMemEval"
echo ""

if ! command -v python3 &>/dev/null && ! command -v python &>/dev/null; then
    echo "ERROR: Python required. Install python3 and datasets package."
    exit 1
fi

PYTHON=$(command -v python3 || command -v python)

DATA_DIR="$DATA_DIR" $PYTHON - <<'PYEOF'
import sys
try:
    from datasets import load_dataset
except ImportError:
    print("ERROR: datasets package not found. Run: pip install datasets")
    sys.exit(1)

import json
import os

data_dir = os.environ["DATA_DIR"]
os.makedirs(data_dir, exist_ok=True)

print("Loading LongMemEval dataset from HuggingFace...")
try:
    ds = load_dataset("xiaowu0162/LongMemEval", split="test")
    out_path = os.path.join(data_dir, "longmemeval_test.jsonl")
    with open(out_path, "w") as f:
        for item in ds:
            f.write(json.dumps(item) + "\n")
    print(f"Saved {len(ds)} examples to {out_path}")
except Exception as e:
    print(f"Download failed: {e}")
    print("You can manually download from: https://huggingface.co/datasets/xiaowu0162/LongMemEval")
    sys.exit(1)
PYEOF

echo "Download complete. Dataset in: $DATA_DIR"
