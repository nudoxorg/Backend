package com.example;

/**
 * Utility class for common mathematical operations.
 *
 * <p>All methods are static; this class cannot be instantiated.
 *
 * @author example-author
 * @since 1.0
 */
public final class MathUtils {

    /** Private constructor; not instantiable. */
    private MathUtils() {}

    /**
     * Returns the factorial of {@code n}.
     *
     * <p>Uses iterative computation to avoid stack overflow.
     *
     * @param n the input; must be {@code >= 0}
     * @return {@code n!}
     * @throws IllegalArgumentException if {@code n} is negative
     */
    public static long factorial(int n) {
        if (n < 0) throw new IllegalArgumentException("n must be >= 0, got " + n);
        long result = 1;
        for (int i = 2; i <= n; i++) result *= i;
        return result;
    }

    /**
     * Clamps {@code value} to the range {@code [min, max]}.
     *
     * @param value the value to clamp
     * @param min   the lower bound (inclusive)
     * @param max   the upper bound (inclusive)
     * @return {@code value} if in range, otherwise the nearest bound
     */
    public static int clamp(int value, int min, int max) {
        return Math.max(min, Math.min(max, value));
    }

    /**
     * Returns {@code true} if {@code n} is a prime number.
     *
     * @param n the candidate (may be negative)
     * @return {@code true} iff {@code n} is prime
     */
    public static boolean isPrime(long n) {
        if (n < 2) return false;
        for (long i = 2; i * i <= n; i++) {
            if (n % i == 0) return false;
        }
        return true;
    }

    /**
     * Joins an array of integers into a comma-separated string.
     *
     * @param values zero or more integers to join
     * @return a comma-separated string, or {@code ""} if no values given
     */
    public static String joinInts(int... values) {
        StringBuilder sb = new StringBuilder();
        for (int i = 0; i < values.length; i++) {
            if (i > 0) sb.append(", ");
            sb.append(values[i]);
        }
        return sb.toString();
    }
}
