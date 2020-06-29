#chg-compatible

  $ configure modern

  $ showgraph() {
  >    hg log -G -T "{rev} {desc}: {phase} {bookmarks} {remotenames}" -r "all()"
  > }

hg cloud hide uses the smartlog data from the cloud service.  We must generate this
manually.
  $ gensmartlogdata() {
  >   echo '{ "smartlog": { "nodes": [' > $TESTTMP/usersmartlogdata
  >   hg log -r "sort(0 + draft() + parents(draft()))" \
  >     -T '{ifeq(rev,0,"",",")}\{"node": "{node}", "phase": "{phase}", "author": "test", "date": "{rev}", "message": "{desc}", "parents": [{join(parents % "\"{node}\"", ", ")}], "bookmarks": [{join(bookmarks % "\"{bookmark}\"", ", ")}]}\n' \
  >     >> $TESTTMP/usersmartlogdata
  > echo "]}}" >> $TESTTMP/usersmartlogdata
  > }

  $ newserver server
  $ cd $TESTTMP/server
  $ drawdag <<EOS
  > W
  > |
  > X Y
  > |/
  > Z
  > EOS
  $ hg bookmark -r $W master
  $ hg bookmark -r $Y other

  $ cd $TESTTMP
  $ clone server client1
  $ cd client1
  $ hg update -q 'desc(Y)'
  $ hg pull -B other
  pulling from ssh://user@dummy/server
  no changes found

  $ drawdag <<EOS
  >             S
  >             |
  >     F       R
  >     |   O   |
  > C D E   |   Q
  >  \|/    N   |
  >   B     |   P
  >   |     M   |
  >   A     |   $Y
  >   |     $Y
  >   $W
  > EOS
  $ hg book -r $D d-bookmark
  $ hg book -r $D d-bookmark2
  $ hg book -r $D d-bookmark3
  $ hg book -r $X x-bookmark
  $ hg book -r $N n-bookmark
  $ hg book -r $N n-bookmark2
  $ hg book -r $O o-bookmark
  $ hg book -r $B b-bookmark
  $ hg cloud sync -q
  $ showgraph
  o  16 S: draft
  |
  | o  15 F: draft
  | |
  o |  14 R: draft
  | |
  | | o  13 O: draft o-bookmark
  | | |
  | o |  12 E: draft
  | | |
  | +---o  11 D: draft d-bookmark d-bookmark2 d-bookmark3
  | | |
  | +---o  10 C: draft
  | | |
  o | |  9 Q: draft
  | | |
  | | o  8 N: draft n-bookmark n-bookmark2
  | | |
  | o |  7 B: draft b-bookmark
  | | |
  o | |  6 P: draft
  | | |
  +---o  5 M: draft
  | |
  | o  4 A: draft
  | |
  | o  3 W: public  remote/master
  | |
  @ |  2 Y: public  remote/other
  | |
  | o  1 X: public x-bookmark
  |/
  o  0 Z: public
  

Remove by hash with two related commits removes both of them
  $ gensmartlogdata
  $ hg cloud hide $P $R
  removing heads:
      0d5fa5021fb8  S
  $ hg cloud sync -q
  $ showgraph
  o  15 F: draft
  |
  | o  13 O: draft o-bookmark
  | |
  o |  12 E: draft
  | |
  +---o  11 D: draft d-bookmark d-bookmark2 d-bookmark3
  | |
  +---o  10 C: draft
  | |
  | o  8 N: draft n-bookmark n-bookmark2
  | |
  o |  7 B: draft b-bookmark
  | |
  | o  5 M: draft
  | |
  o |  4 A: draft
  | |
  o |  3 W: public  remote/master
  | |
  | @  2 Y: public  remote/other
  | |
  o |  1 X: public x-bookmark
  |/
  o  0 Z: public
  

Remove by hash removes commit, all descendants and their bookmarks
  $ gensmartlogdata
  $ hg cloud hide $N
  removing heads:
      7f49e3f0c6cd  O
  adding heads:
      9c4fc22fed7c  M
  removing bookmarks:
      n-bookmark: d3adf05d12fa
      n-bookmark2: d3adf05d12fa
      o-bookmark: 7f49e3f0c6cd
  $ hg cloud sync -q
  $ showgraph
  o  15 F: draft
  |
  o  12 E: draft
  |
  | o  11 D: draft d-bookmark d-bookmark2 d-bookmark3
  |/
  | o  10 C: draft
  |/
  o  7 B: draft b-bookmark
  |
  | o  5 M: draft
  | |
  o |  4 A: draft
  | |
  o |  3 W: public  remote/master
  | |
  | @  2 Y: public  remote/other
  | |
  o |  1 X: public x-bookmark
  |/
  o  0 Z: public
  

Remove when other heads keep ancestors alive, removing it just removes the head
  $ gensmartlogdata
  $ hg cloud hide $C
  removing heads:
      f6a18bc998c9  C
  $ hg cloud sync -q
  $ showgraph
  o  15 F: draft
  |
  o  12 E: draft
  |
  | o  11 D: draft d-bookmark d-bookmark2 d-bookmark3
  |/
  o  7 B: draft b-bookmark
  |
  | o  5 M: draft
  | |
  o |  4 A: draft
  | |
  o |  3 W: public  remote/master
  | |
  | @  2 Y: public  remote/other
  | |
  o |  1 X: public x-bookmark
  |/
  o  0 Z: public
  

Remove by bookmark leaves commits alone if there are other bookmarks
  $ gensmartlogdata
  $ hg cloud hide -B d-bookmark
  removing bookmarks:
      d-bookmark: fa9d7a2f38d1
  $ hg cloud sync -q
  $ showgraph
  o  15 F: draft
  |
  o  12 E: draft
  |
  | o  11 D: draft d-bookmark2 d-bookmark3
  |/
  o  7 B: draft b-bookmark
  |
  | o  5 M: draft
  | |
  o |  4 A: draft
  | |
  o |  3 W: public  remote/master
  | |
  | @  2 Y: public  remote/other
  | |
  o |  1 X: public x-bookmark
  |/
  o  0 Z: public
  

But removing all of the bookmarks pointing to a head removes the head
  $ gensmartlogdata
  $ hg cloud hide -B "re:d-bookmark.*"
  removing heads:
      fa9d7a2f38d1  D
  removing bookmarks:
      d-bookmark2: fa9d7a2f38d1
      d-bookmark3: fa9d7a2f38d1
  $ hg cloud sync -q
  $ showgraph
  o  15 F: draft
  |
  o  12 E: draft
  |
  o  7 B: draft b-bookmark
  |
  | o  5 M: draft
  | |
  o |  4 A: draft
  | |
  o |  3 W: public  remote/master
  | |
  | @  2 Y: public  remote/other
  | |
  o |  1 X: public x-bookmark
  |/
  o  0 Z: public
  

Removing a bookmark in the stack doesn't hide the commit
  $ gensmartlogdata
  $ hg cloud hide -B b-bookmark
  removing bookmarks:
      b-bookmark: 9272e7e427bf
  $ hg cloud sync -q
  $ showgraph
  o  15 F: draft
  |
  o  12 E: draft
  |
  o  7 B: draft
  |
  | o  5 M: draft
  | |
  o |  4 A: draft
  | |
  o |  3 W: public  remote/master
  | |
  | @  2 Y: public  remote/other
  | |
  o |  1 X: public x-bookmark
  |/
  o  0 Z: public
  

Removing a bookmark on a public commit just removes it
  $ gensmartlogdata
  $ hg cloud hide -B x-bookmark
  removing bookmarks:
      x-bookmark: 8a0aebad5927
  $ hg cloud sync -q
  $ showgraph
  o  15 F: draft
  |
  o  12 E: draft
  |
  o  7 B: draft
  |
  | o  5 M: draft
  | |
  o |  4 A: draft
  | |
  o |  3 W: public  remote/master
  | |
  | @  2 Y: public  remote/other
  | |
  o |  1 X: public
  |/
  o  0 Z: public
  

Removing a lone commit just removes that head
  $ gensmartlogdata
  $ hg cloud hide $M
  removing heads:
      9c4fc22fed7c  M
  $ hg cloud sync -q
  $ showgraph
  o  15 F: draft
  |
  o  12 E: draft
  |
  o  7 B: draft
  |
  o  4 A: draft
  |
  o  3 W: public  remote/master
  |
  | @  2 Y: public  remote/other
  | |
  o |  1 X: public
  |/
  o  0 Z: public
  

Removing a remote bookmark works
  $ gensmartlogdata
  $ hg cloud hide --remotebookmark remote/other
  removing remote bookmarks:
      remote/other: 1cab361770de
  $ hg cloud sync -q
  $ showgraph
  o  15 F: draft
  |
  o  12 E: draft
  |
  o  7 B: draft
  |
  o  4 A: draft
  |
  o  3 W: public  remote/master
  |
  | @  2 Y: draft
  | |
  o |  1 X: public
  |/
  o  0 Z: public
  

BUG! Commit Y is now draft - it should've been hidden

Merge commits can be removed
  $ drawdag <<EOS
  >  H H
  >  | |
  >  | G
  >  | |
  >  | $A
  >  |
  >  $F
  > EOS
  $ hg cloud sync -q
  $ showgraph
  o    18 H: draft
  |\
  | o  17 G: draft
  | |
  o |  15 F: draft
  | |
  o |  12 E: draft
  | |
  o |  7 B: draft
  |/
  o  4 A: draft
  |
  o  3 W: public  remote/master
  |
  | @  2 Y: draft
  | |
  o |  1 X: public
  |/
  o  0 Z: public
  

  $ gensmartlogdata
  $ hg cloud hide $H
  removing heads:
      28730d10073d  H
  adding heads:
      080f94b3ed7f  F
      e59c81d53e06  G
  $ hg cloud sync -q
  $ showgraph
  o  17 G: draft
  |
  | o  15 F: draft
  | |
  | o  12 E: draft
  | |
  | o  7 B: draft
  |/
  o  4 A: draft
  |
  o  3 W: public  remote/master
  |
  | @  2 Y: draft
  | |
  o |  1 X: public
  |/
  o  0 Z: public
  

