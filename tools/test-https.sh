#!/bin/sh
# test-https.sh - end-to-end HTTPS in QEMU: æther's browser fetches a page over
# TLS 1.3 from a server on the host, through æther's TCP/IP stack, DNS and
# firewall. Uses a throwaway CA trusted only by a TEST build of the browser,
# which this script replaces with the normal build at the end.
#
#   tools/test-https.sh        (needs root for ports 53/443 and /etc/resolv.conf)
set -e
cd "$(dirname "$0")/.."
work=build/https-test
rm -rf "$work" && mkdir -p "$work/www"
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -keyout "$work/ca.key" -out "$work/ca.pem" \
    -days 2 -subj "/CN=aether test CA" -addext "basicConstraints=critical,CA:TRUE" \
    -addext "keyUsage=critical,keyCertSign" 2>/dev/null
openssl req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -keyout "$work/srv.key" -out "$work/srv.csr" \
    -subj "/CN=aether.test" 2>/dev/null
printf "subjectAltName=DNS:aether.test\nbasicConstraints=CA:FALSE\nextendedKeyUsage=serverAuth\n" > "$work/ext.cnf"
openssl x509 -req -in "$work/srv.csr" -CA "$work/ca.pem" -CAkey "$work/ca.key" -CAcreateserial \
    -out "$work/srv.pem" -days 2 -extfile "$work/ext.cnf" 2>/dev/null
cat > "$work/www/index.html" <<'HTML'
<!doctype html><html><head><title>Hello over TLS</title>
<style>body{max-width:40em;margin:2em auto}h1{color:#38e1f5}.box{border:1px solid #888;padding:1em;border-radius:8px}</style></head>
<body><h1>It works over HTTPS</h1>
<p class="box">This page came through the browser's own <b>TLS 1.3</b> and æther's <b>TCP/IP stack</b> and <b>firewall</b>, all written from scratch.</p>
<ul><li>Certificate checked against a root</li><li>Host name matched</li><li id="js">Scripts: not run</li></ul>
<p id="fetched">Fetch: waiting</p>
<p><a href="/second.html">A second page</a></p>
<script>document.getElementById('js').textContent = 'Scripts: the inline script ran';</script>
<script src="/app.js"></script></body></html>
HTML
cat > "$work/www/app.js" <<'JS'
fetch('/data.json').then(r => r.json()).then(d => {
    document.getElementById('fetched').textContent = 'Fetch: ' + d.message;
    console.log('fetched ' + d.message);
}, e => { document.getElementById('fetched').textContent = 'Fetch failed: ' + e.message; });
JS
printf '{"message": "same-origin fetch over TLS works"}\n' > "$work/www/data.json"
printf '<html><head><title>Second page</title></head><body><h2>Second page</h2><p>Links work too.</p></body></html>\n' \
    > "$work/www/second.html"

tools/build-browser.sh --test-root "$work/ca.pem" >/dev/null
make >/dev/null

cp /etc/resolv.conf "$work/resolv.conf"
cleanup() {
    cp "$work/resolv.conf" /etc/resolv.conf
    kill $DNS $TLS 2>/dev/null || true
}
trap cleanup EXIT
python3 tools/test-dns.py & DNS=$!
(cd "$work/www" && exec openssl s_server -quiet -accept 443 -cert ../srv.pem -key ../srv.key -tls1_3 -WWW) & TLS=$!
echo "nameserver 127.0.0.1" > /etc/resolv.conf
sleep 1
status=0
AETHER_DISK=none AETHER_NIC_OPTS=net=203.0.113.0/24 tools/qemu-drive.py tests/https.txt build/test-https || status=1
cleanup
trap - EXIT
tools/build-browser.sh >/dev/null # back to the normal build, which trusts only the built-in roots
make >/dev/null
exit $status
