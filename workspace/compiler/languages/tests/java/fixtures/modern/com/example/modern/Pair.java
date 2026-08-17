package com.example.modern;

import java.util.Objects;

/**
 * A generic, bounded-type-parameter pair.
 *
 * @param <A> the first element's type; must be {@link Comparable} with itself
 * @param <B> the second element's type
 * @param first the first element
 * @param second the second element
 */
public record Pair<A extends Comparable<A>, B>(A first, B second) {
    public Pair {
        Objects.requireNonNull(first);
    }
}
