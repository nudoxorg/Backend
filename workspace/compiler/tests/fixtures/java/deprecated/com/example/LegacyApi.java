package com.example;

/**
 * A class with deprecated members, to exercise {@code @Deprecated} lowering.
 *
 * @deprecated Use {@code ModernApi} instead.
 */
@Deprecated
public class LegacyApi {

    /**
     * A deprecated constant.
     *
     * @deprecated No longer meaningful.
     */
    @Deprecated
    public static final int OLD_MAX = 100;

    /**
     * A deprecated method.
     *
     * @deprecated Use {@code newProcess(String)} instead.
     * @param input the input string
     * @return the processed result
     */
    @Deprecated
    public String oldProcess(String input) {
        return input.trim();
    }

    /**
     * The replacement method (not deprecated).
     *
     * @param input the input string
     * @return the processed result
     */
    public String newProcess(String input) {
        return input.strip();
    }
}
