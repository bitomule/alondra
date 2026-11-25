"""Where the private literals of this Mac are read from."""

NONE = "none"
_RELATIVE = "files/private-literals.txt"

def literals_source(declared, alondra_home, home):
    """Picks the file or marker the private literals come from.

    Args:
      declared: value of ALONDRA_PRIVATE_LITERALS, empty when not set.
      alondra_home: value of ALONDRA_HOME, empty when not set.
      home: value of HOME, empty when not set.

    Returns:
      A struct with `path` (empty when there is nowhere to look) and `required`: a declared file
      must exist, the default one only counts when it does.
    """
    if declared:
        return struct(path = declared, required = declared != NONE)
    if alondra_home:
        return struct(path = "%s/%s" % (alondra_home.rstrip("/"), _RELATIVE), required = False)
    if home:
        return struct(path = "%s/.alondra/%s" % (home.rstrip("/"), _RELATIVE), required = False)
    return struct(path = "", required = False)
