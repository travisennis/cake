"""Exercise the renamed helper across all callers."""

import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "src"))

import app
import report


def test_rename():
    assert app.render(1234) == "$12.34"
    assert report.total_line([100, 250]) == "$3.50"


if __name__ == "__main__":
    test_rename()
    print("ok")
