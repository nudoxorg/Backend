package parity;

/** Computes a value. */
interface Service<T> { T run(T value); }

/** Implements the service. */
final class Worker implements Service<String> {
    public String run(String value) { return value; }
}
