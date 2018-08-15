  $ . helpers-usechg.sh
  $ enable fbamend inhibit rebase
  $ setconfig rebase.experimental.inmemory=True
  $ setconfig rebase.singletransaction=True
  $ setconfig experimental.evolution.allowdivergence=True
  $ setconfig experimental.evolution="createmarkers, allowunstable"
  $ setconfig amend.autorestack=no-conflict
  $ mkcommit() {
  >   echo "$1" > "$1"
  >   hg add "$1"
  >   hg ci -m "add $1"
  > }

Test invalid value for amend.autorestack
  $ newrepo
  $ hg debugdrawdag<<'EOS'
  >    D
  >    |
  > C  C_old
  > |  |      # amend: B_old -> B
  > B  B_old  # amend: C_old -> C
  > | /
  > |/
  > A
  > EOS
  $ hg update -qC B
  $ echo "new content" > B
  $ showgraph
  o  5 3c36beb5705f D
  |
  x  4 07863d11c289 C_old
  |
  | o  3 26805aba1e60 C
  | |
  x |  2 3326d5194fc9 B_old
  | |
  | @  1 112478962961 B
  |/
  o  0 426bada5c675 A
  $ hg amend -m "B'"
  restacking children automatically (unless they conflict)
  rebasing 3:26805aba1e60 "C" (C)
  rebasing 5:3c36beb5705f "D" (D)

BUG: D is rebased onto B':
  $ showgraph
  o  8 4664373842d7 D
  |
  | o  7 5676eb48a524 C
  |/
  @  6 180681c3ccd0 B'
  |
  | x  5 3c36beb5705f D
  | |
  | x  4 07863d11c289 C_old
  | |
  | | x  3 26805aba1e60 C
  | | |
  | x |  2 3326d5194fc9 B_old
  |/ /
  | x  1 112478962961 B
  |/
  o  0 426bada5c675 A
  $ hg rebase --restack
  nothing to rebase - empty destination
  $ showgraph
  o  8 4664373842d7 D
  |
  | o  7 5676eb48a524 C
  |/
  @  6 180681c3ccd0 B'
  |
  | x  5 3c36beb5705f D
  | |
  | x  4 07863d11c289 C_old
  | |
  | | x  3 26805aba1e60 C
  | | |
  | x |  2 3326d5194fc9 B_old
  |/ /
  | x  1 112478962961 B
  |/
  o  0 426bada5c675 A
