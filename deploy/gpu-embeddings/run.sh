#!/bin/bash
# Installs the GPU runtime embed.py needs, then runs it.
# Usage: bash run.sh <https URL of a UTF-8 text file>
#
# ONNX Runtime 1.20 is the last release built for every CUDA 12.x, so it loads
# on the workspace image's CUDA 12.4 libraries; 1.21 and later need 12.8. The
# image already ships cuBLAS, so cuDNN is installed without its own copy.
set -euo pipefail
stage=setup
phase() {
  stage=$1
  printf '[covenant-embedding] phase=%s elapsed_secs=%s\n' "$stage" "$SECONDS" >&2
}
trap 'code=$?; printf "[covenant-embedding] phase=%s exit_code=%s elapsed_secs=%s\n" "$stage" "$code" "$SECONDS" >&2' EXIT
# Images with an externally managed Python refuse plain pip installs; this
# machine is rented for this job alone.
export PIP_BREAK_SYSTEM_PACKAGES=1
pip_install() {
  python3 -m pip install --quiet --disable-pip-version-check --no-cache-dir "$@"
}
phase install_python_dependencies
pip_install onnxruntime-gpu==1.20.2 tokenizers==0.22.1 numpy==2.2.6
phase install_cudnn
pip_install --no-deps nvidia-cudnn-cu12==9.5.1.17
phase configure_cuda_libraries
cudnn_lib=$(python3 -c 'import nvidia.cudnn, os; print(os.path.join(list(nvidia.cudnn.__path__)[0], "lib"))')
export LD_LIBRARY_PATH="$cudnn_lib:${LD_LIBRARY_PATH:-}"
phase embedding_workload
python3 -u embed.py "$1"
phase complete
