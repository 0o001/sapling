import time, sys, os, random


def waithook(ui, repo, **kwargs):
    """This hook is used to block pushes in some pushrebase tests

    It spins until `.hg/flag` exists
    """
    start = time.time()
    repo._wlockfreeprefix.add("hookrunning")
    repo.vfs.write("hookrunning", "")
    while not repo.vfs.exists("flag"):
        if time.time() - start > 20:
            print >>sys.stderr, "ERROR: Timeout waiting for .hg/flag"
            repo.vfs.unlink("hookrunning")
            return True
        time.sleep(0.05)
    repo.vfs.unlink("hookrunning")
    return False
