"""Tests for catalog helpers."""

import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "src"))

import catalog


def test_page_size():
    assert catalog.page_size() == 50


if __name__ == "__main__":
    test_page_size()
    print("ok")
