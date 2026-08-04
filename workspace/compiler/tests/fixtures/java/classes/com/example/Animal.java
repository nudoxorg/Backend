package com.example;

/**
 * An abstract base class representing an animal.
 * All animals have a name and can make a sound.
 */
public abstract class Animal {

    /** The animal's name. */
    private String name;

    /** The animal's age in years. */
    protected int age;

    /**
     * Constructs an Animal with the given name and age.
     *
     * @param name the animal's name
     * @param age  the animal's age
     */
    public Animal(String name, int age) {
        this.name = name;
        this.age = age;
    }

    /** Returns this animal's name. */
    public String getName() {
        return name;
    }

    /** Returns this animal's age. */
    public int getAge() {
        return age;
    }

    /**
     * Returns the sound this animal makes.
     *
     * @return the sound string
     */
    public abstract String sound();

    /**
     * Returns a human-readable description.
     *
     * @param verbose include extra detail when {@code true}
     * @return description string
     */
    public String describe(boolean verbose) {
        if (verbose) {
            return name + " (age " + age + ") says: " + sound();
        }
        return name + " says: " + sound();
    }
}
