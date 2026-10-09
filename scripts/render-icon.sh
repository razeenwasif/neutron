#!/bin/sh
# Run `npm ci --prefix scripts` first. Also requires ImageMagick 7.
set -eu
node "$(dirname "$0")/render-icon.cjs"
