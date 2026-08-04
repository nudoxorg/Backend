package com.example;

/**
 * Cardinal compass directions.
 */
public enum Direction {
    /** True north. */
    NORTH,
    /** Due south. */
    SOUTH,
    /** Due east. */
    EAST,
    /** Due west. */
    WEST;

    /**
     * Returns the opposite direction.
     *
     * @return the direction directly opposite to this one
     */
    public Direction opposite() {
        return switch (this) {
            case NORTH -> SOUTH;
            case SOUTH -> NORTH;
            case EAST  -> WEST;
            case WEST  -> EAST;
        };
    }
}
