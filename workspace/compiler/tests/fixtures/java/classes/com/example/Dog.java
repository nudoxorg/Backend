package com.example;

/**
 * A concrete dog, extending {@link Animal}.
 */
public class Dog extends Animal {

    /** The dog's breed. */
    private String breed;

    /** Whether this dog is trained. */
    boolean trained;  // package-private

    /**
     * Constructs a Dog.
     *
     * @param name   the dog's name
     * @param age    the dog's age
     * @param breed  the dog's breed
     */
    public Dog(String name, int age, String breed) {
        super(name, age);
        this.breed = breed;
    }

    /** Returns the breed. */
    public String getBreed() {
        return breed;
    }

    @Override
    public String sound() {
        return "woof";
    }

    /**
     * Teaches the dog a trick.
     *
     * @param trick the name of the trick to learn
     * @throws IllegalArgumentException if the trick name is blank
     */
    public void learnTrick(String trick) {
        if (trick == null || trick.isBlank()) {
            throw new IllegalArgumentException("Trick name must not be blank");
        }
        trained = true;
    }

    /** A package-private helper. */
    void internalReset() {
        trained = false;
    }

    /** Private implementation detail. */
    private void bark(int times) {
        for (int i = 0; i < times; i++) {
            System.out.println("Woof!");
        }
    }
}
