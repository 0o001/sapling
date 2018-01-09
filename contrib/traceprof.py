#!/usr/bin/env python

from __future__ import absolute_import, print_function

import os
import sys

from hgext import traceprof
from mercurial import ui as uimod

if __name__ == '__main__':
    sys.argv = sys.argv[1:]
    if not sys.argv:
        print("usage: traceprof.py <script> <arguments...>", file=sys.stderr)
        sys.exit(2)
    sys.path.insert(0, os.path.abspath(os.path.dirname(sys.argv[0])))
    u = uimod.ui()
    u.setconfig('traceprof', 'timethreshold', 0)
    with traceprof.profile(u, sys.stderr):
        execfile(sys.argv[0])
