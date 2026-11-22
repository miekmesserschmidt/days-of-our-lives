#!/bin/sh
# The smallest possible days-of-our-lives plugin. Install as `dool-plugin-hello`.
case "$1" in
  --describe) echo '{"name":"hello","description":"says hi","protocol":1,"version":"0.1.0"}'; exit;;
  --default-config) printf 'plugin = "hello"\n[position]\nbar = "main"\n'; exit;;
esac
while read -r line; do          # exits on EOF
  echo '{"type":"svg","svg":"<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 60 20\"><text x=\"2\" y=\"15\" fill=\"white\">hi</text></svg>"}'
done
