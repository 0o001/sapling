#chg-compatible

#if no-windows

  $ disable treemanifest
  $ configure dummyssh
#require serve
#require bucktest

  $ hg init test
  $ cd test

  $ echo foo>foo
  $ hg addremove
  adding foo
  $ hg commit -m 1

  $ hg verify
  warning: verify does not actually check anything in this repo

  $ cert="${HGTEST_CERTDIR}/localhost.crt"
  $ cert_key="${HGTEST_CERTDIR}/localhost.key"
  $ PROXY_PORT=1338

  $ printf "HTTP/1.1 401 Unauthorized\r\nX-FB-Validated-X2PAuth-Advice-denied-request: advice here\r\n\r\n" | ncat -lkv --ssl-cert "$cert" --ssl-key "$cert_key" localhost "$PROXY_PORT" 1>/dev/null 2>/dev/null &
  $ to_kill=$!
  $ hg pull --insecure --config paths.default=mononoke://localhost:$PROXY_PORT/test --config auth.mononoke.cert=$cert --config auth.mononoke.key=$cert_key --config auth.mononoke.prefix=mononoke://*
  pulling from mononoke://localhost:1338/test
  warning: connection security to localhost is disabled per current settings; communication is susceptible to eavesdropping and tampering
  abort: unexpected server response: "401 Unauthorized": advice here!
  [255]
  $ kill $to_kill

#endif
