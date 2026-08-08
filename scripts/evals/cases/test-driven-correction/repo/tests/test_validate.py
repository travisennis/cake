"""Tests for email validation."""

import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "src"))

import validate


def test_valid_emails():
    assert validate.is_valid_email("alice@example.com")
    assert validate.is_valid_email("bob.smith@example.co.uk")


def test_invalid_emails():
    assert not validate.is_valid_email("not-an-email")
    assert not validate.is_valid_email("missing@")
    assert not validate.is_valid_email("@example.com")
    assert not validate.is_valid_email("spaces in@example.com")


if __name__ == "__main__":
    test_valid_emails()
    test_invalid_emails()
    print("ok")
