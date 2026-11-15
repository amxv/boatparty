from app import Point


def test_uses_the_dependency():
    assert Point(1, 2) == Point(1, 2)
