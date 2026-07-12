package com.example;

/**
 * Exercises all four Java visibility modifiers.
 */
public class Visibility {

    /** Public field — visible everywhere. */
    public int publicField;

    /** Protected field — visible to subclasses and same package. */
    protected int protectedField;

    /** Package-private field — visible only in the same package. */
    int packageField;

    /** Private field — visible only within this class. */
    private int privateField;

    /** Public method. */
    public void publicMethod() {}

    /** Protected method. */
    protected void protectedMethod() {}

    /** Package-private method. */
    void packageMethod() {}

    /** Private method. */
    private void privateMethod() {}
}
