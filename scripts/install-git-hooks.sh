#!/usr/bin/env sh

set -eu

git config core.hooksPath .githooks
echo "Git hooks installed. core.hooksPath -> .githooks"
