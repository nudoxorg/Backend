package com.example;

/**
 * Lifecycle status for an entity.
 */
public enum Status {
    PENDING,
    ACTIVE,
    INACTIVE,
    DELETED;

    /** Returns {@code true} if this status is terminal (cannot advance). */
    public boolean isTerminal() {
        return this == DELETED;
    }
}
