#!/usr/bin/env bash
set -e
duration=1800
sleep=300
bash run.sh -d "${duration}" -c " --max-bytes-for-level-base 536870912 --max-bytes-for-level-multiplier 10 "
sleep ${sleep}
bash run.sh -d "${duration}" -c " --max-bytes-for-level-base 536870912 --max-bytes-for-level-multiplier 10 " -g
#sleep ${sleep}
#bash run.sh -d "${duration}" -c " --max-bytes-for-level-base 2147483648 --max-bytes-for-level-multiplier 5 "
#sleep ${sleep}
#bash run.sh -d "${duration}" -c " --max-bytes-for-level-base 2147483648 --max-bytes-for-level-multiplier 5 " -g
