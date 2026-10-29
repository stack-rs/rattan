#! /bin/bash
set -e


SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" &> /dev/null && pwd -P)"

if [ -x "${SCRIPT_DIR}/../target/release/drift-matcher" ]; then
    echo "Found drift-matcher executable"
else
    echo "Build drift-matcher executable"
    (cd ${SCRIPT_DIR}/.. && cargo build --release --package rattan-log --bin drift-matcher)
fi

DRIFT_MATCHER=$(realpath ${SCRIPT_DIR}/../target/release/drift-matcher)

ls -l $DRIFT_MATCHER


DIR=${1:-.}


find "$DIR" -type d -regextype posix-extended \
    -regex '.*/[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}' \
    -print |
while IFS= read -r ARTIFACTS_DIR; do
    $DRIFT_MATCHER "${ARTIFACTS_DIR}" > "${ARTIFACTS_DIR}/drift.log"
done