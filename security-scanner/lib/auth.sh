#!/bin/bash
# Auth helper - builds authentication arguments for Nuclei

build_auth_args() {
    local token="$1"
    local cookie="$2"
    local headers="$3"
    local args=""

    # Bearer/API token
    if [ -n "$token" ]; then
        args="${args} -H 'Authorization: Bearer ${token}'"
    fi

    # Session cookie
    if [ -n "$cookie" ]; then
        args="${args} -H 'Cookie: ${cookie}'"
    fi

    # Custom headers
    if [ -n "$headers" ]; then
        while IFS= read -r header; do
            if [ -n "$header" ]; then
                args="${args} -H '${header}'"
            fi
        done <<< "$(echo -e "$headers")"
    fi

    echo "$args"
}
