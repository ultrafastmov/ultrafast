"""Tests: python3 -m unittest discover -s tests -t . (from sdk/python).

They import the package from ../src unless ULTRAFAST_TEST_INSTALLED=1, which
tests whatever ``ultrafastmov`` is installed instead.
"""

import os
import sys

if not os.environ.get("ULTRAFAST_TEST_INSTALLED"):
    sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "src"))
