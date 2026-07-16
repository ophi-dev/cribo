if __name__ != "__main__":
    import partial_observer

    target = partial_observer.FailingTarget()
    try:
        ASSIGNED = target.value = "assigned"
    except RuntimeError:
        assert target.assignment_visible

    DELETED = "deleted"
    try:
        del DELETED, target.value
    except RuntimeError:
        assert target.deletion_applied

    direct_target = partial_observer.FailingUnpackTarget(
        (("UNPACKED_FIRST", "first"),)
    )
    try:
        UNPACKED_FIRST, direct_target.value = ("first", "ignored")
    except RuntimeError:
        assert direct_target.unpacking_visible

    nested_target = partial_observer.FailingUnpackTarget(
        (("NESTED_OUTER", "outer"), ("NESTED_INNER", "inner"))
    )
    try:
        NESTED_OUTER, (NESTED_INNER, nested_target.value) = (
            "outer",
            ("inner", "ignored"),
        )
    except RuntimeError:
        assert nested_target.unpacking_visible

    starred_target = partial_observer.FailingUnpackTarget(
        (("STAR_FIRST", "first"), ("STAR_REST", ["middle"]))
    )
    try:
        STAR_FIRST, *STAR_REST, starred_target.value = (
            "first",
            "middle",
            "ignored",
        )
    except RuntimeError:
        assert starred_target.unpacking_visible

    ASSIGNMENT_VISIBLE = target.assignment_visible
    DELETION_APPLIED = target.deletion_applied
    DIRECT_UNPACKING_VISIBLE = direct_target.unpacking_visible
    NESTED_UNPACKING_VISIBLE = nested_target.unpacking_visible
    STARRED_UNPACKING_VISIBLE = starred_target.unpacking_visible
