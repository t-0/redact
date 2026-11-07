#!/bin/bash
# The `if` below is never closed, and $name is unquoted and unset.
name=$1
if [ -z $name ]; then
    echo "need a name"
    for i in 1 2 3; do
        echo $i
done
