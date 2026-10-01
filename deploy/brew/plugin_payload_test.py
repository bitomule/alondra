#!/usr/bin/env python3
"""Every file of the plugins Alondra launches its nodes and orchestrator with ships in the Brew release."""
import importlib.util
import sys

spec = importlib.util.spec_from_file_location('release', sys.argv[1])
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)
missing = sorted(set(sys.argv[2:]) - release.PUBLIC_FILES)
if missing:
    print('The Brew release would leave out these plugin files: ' + ', '.join(missing), file=sys.stderr)
    sys.exit(1)
