#!/usr/bin/env bash
# Run the LP lifecycle (join -> gamm swap volume -> exit) on all 5 pools.
D="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
{ echo ""; echo "===== LP FUNCTIONALITY (all 5 pools) ====="; } >> "$(cd "$D/.." && pwd)/liverun_results.log"
bash "$D/run_lp.sh" osmo1nwf9pn96dkezpc5z3586y35rla6jq67gmevkczpcu4kyc8tup39qy3mjcq lpone
bash "$D/run_lp.sh" osmo1wne8umvyqy2am2n423u35waekfe3up4m7yls0rwwhct8jzr0h5lqgwpztx lptwo
bash "$D/run_lp.sh" osmo19l8ec2tty8w9w6kspt5ff9g59yd7ftf5qky468zmmt3u4zkgrz9svy2lzy lptre
bash "$D/run_lp.sh" osmo1d8fdkh783jn28p9aeglnx7378a64d42ms8q068ckhfpwrjh8r3ksusrjpr lpfor
bash "$D/run_lp.sh" osmo1dg0n3taw8w8aywfzqaxz8mkvupny027lwan78rvrj6hjquny95psyh4f9z lpfiv
echo "LP ALL DONE" >> "$(cd "$D/.." && pwd)/liverun_results.log"
