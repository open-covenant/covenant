#!/bin/bash
# Installs the GPU runtime embed.py needs, then runs it.
# Usage: bash run.sh <https URL of a UTF-8 text file>
#
# ONNX Runtime 1.20 is the last release built for every CUDA 12.x, so it loads
# on the workspace image's CUDA 12.4 libraries; 1.21 and later need 12.8. The
# image already ships cuBLAS, so cuDNN is installed without its own copy.
set -euo pipefail
pip_install() {
  python3 -m pip install --quiet --disable-pip-version-check --no-cache-dir "$@"
}
pip_install onnxruntime-gpu==1.20.2 tokenizers==0.22.1 numpy==2.2.6
pip_install --no-deps nvidia-cudnn-cu12==9.5.1.17
cudnn_lib=$(python3 -c 'import nvidia.cudnn, os; print(os.path.join(list(nvidia.cudnn.__path__)[0], "lib"))')
export LD_LIBRARY_PATH="$cudnn_lib:${LD_LIBRARY_PATH:-}"
python3 embed.py "$1"
