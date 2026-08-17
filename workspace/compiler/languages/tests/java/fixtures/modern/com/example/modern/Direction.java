package com.example.modern;

/** The four cardinal directions, each carrying its opposite's ordinal offset. */
public enum Direction {
    NORTH(2),
    EAST(2),
    SOUTH(2),
    WEST(2);

    private final int oppositeOffset;

    Direction(int oppositeOffset) {
        this.oppositeOffset = oppositeOffset;
    }

    /**
     * The opposite direction.
     * @return the opposite of this direction
     */
    public Direction opposite() {
        int idx = (ordinal() + oppositeOffset) % values().length;
        return values()[idx];
    }
}
