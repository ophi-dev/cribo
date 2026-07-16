if __name__ != "__main__":
    import partial_observer

    target = partial_observer.FailingTarget()
    try:
        ASSIGNED = target.value = "assigned"
    except RuntimeError:
        pass

    DELETED = "deleted"
    try:
        del DELETED, target.value
    except RuntimeError:
        pass

    ASSIGNMENT_VISIBLE = target.assignment_visible
    DELETION_APPLIED = target.deletion_applied
