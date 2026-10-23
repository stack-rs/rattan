#! /bin/bash
set -e


SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" &> /dev/null && pwd -P)"

# if [ -x "${SCRIPT_DIR}/../target/release/rattan-log" ]; then
#     echo "Found rattan-log executable"
# else
#     echo "Build rattan-log executable"
#     (cd ${SCRIPT_DIR}/.. && cargo build --release --package rattan-log)
# fi

RATTAN_LOG=$(realpath ${SCRIPT_DIR}/../target/release/rattan-log)

ls -l $RATTAN_LOG


DIR=${1:-.}


find "$DIR" -type d -regextype posix-extended \
    -regex '.*/[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}' \
    -print |
while IFS= read -r ARTIFACTS_DIR; do
    ARTIFACTS_DIR=${ARTIFACTS_DIR} $RATTAN_LOG > ${ARTIFACTS_DIR}/drift.log
done