#chg-compatible
#debugruntest-compatible
#inprocess-hg-incompatible

  $ hg init repo
  $ cd repo
  $ export EDENSCM_LOG=auth=debug

No credentials

  $ hg debugreadauthforuri https://example.com
  no match found

Single credential

  $ cat > "$HGRCPATH" << EOF
  > [auth]
  > first.prefix=example.com
  > first.schemes=https
  > EOF

  $ hg debugreadauthforuri https://example.com
  auth.first.prefix=example.com
  auth.first.schemes=https

Non-existent credentials are ignored

  $ cat > "$HGRCPATH" << EOF
  > [auth]
  > first.cert=foocert
  > first.prefix=example.com
  > first.schemes=https
  > second.key=fookey
  > second.prefix=example.com
  > second.schemes=https
  > EOF

  $ hg debugreadauthforuri https://example.com
  DEBUG auth: Ignoring [auth] group "first" because of missing client certificate: "foocert"
  DEBUG auth: Ignoring [auth] group "second" because of missing private key: "fookey"
  no match found

Valid credentials are used

  $ touch foocert
  $ cat > "$HGRCPATH" << EOF
  > [auth]
  > first.cert=foocert
  > first.prefix=example.com
  > first.schemes=https
  > EOF

  $ hg debugreadauthforuri https://example.com
  auth.first.cert=foocert
  auth.first.prefix=example.com
  auth.first.schemes=https

Valid credentials are preferred

  $ cat > "$HGRCPATH" << EOF
  > [auth]
  > first.cert=foocert
  > first.prefix=example.com
  > first.schemes=https
  > second.key=fookey
  > second.prefix=example.com
  > second.schemes=https
  > second.priority=1
  > EOF

  $ hg debugreadauthforuri https://example.com
  DEBUG auth: Ignoring [auth] group "second" because of missing private key: "fookey"
  auth.first.cert=foocert
  auth.first.prefix=example.com
  auth.first.schemes=https

Longest prefixes are used

  $ cat > "$HGRCPATH" << EOF
  > [auth]
  > first.prefix=example.com/foo
  > first.schemes=https
  > second.prefix=example.com
  > second.schemes=https
  > EOF

  $ hg debugreadauthforuri https://example.com/foo
  auth.first.prefix=example.com/foo
  auth.first.schemes=https

Prefixes take precedence over priorities

  $ cat > "$HGRCPATH" << EOF
  > [auth]
  > first.prefix=example.com/foo
  > first.schemes=https
  > second.prefix=example.com
  > second.schemes=https
  > second.priority=1
  > EOF

  $ hg debugreadauthforuri https://example.com/foo
  auth.first.prefix=example.com/foo
  auth.first.schemes=https

Priorities take precedence over user names

  $ cat > "$HGRCPATH" << EOF
  > [auth]
  > first.prefix=example.com
  > first.schemes=https
  > first.username=user
  > second.prefix=example.com
  > second.schemes=https
  > second.priority=1
  > EOF

  $ hg debugreadauthforuri https://example.com
  auth.second.prefix=example.com
  auth.second.priority=1
  auth.second.schemes=https

User names are used if everything else matches

  $ cat > "$HGRCPATH" << EOF
  > [auth]
  > first.prefix=example.com
  > first.schemes=https
  > first.username=user
  > first.priority=1
  > second.prefix=example.com
  > second.schemes=https
  > second.priority=1
  > EOF

  $ hg debugreadauthforuri https://example.com
  auth.first.prefix=example.com
  auth.first.priority=1
  auth.first.schemes=https
  auth.first.username=user
