package com.example;

import java.util.HashMap;
import java.util.HashSet;
import java.util.List;
import java.util.Map;
import java.util.Optional;
import java.util.Set;

/**
 * A generic in-memory registry mapping string keys to typed values.
 *
 * <p>Exercises the collection-type mappings: {@code List<T>}, {@code Map<K,V>},
 * {@code Set<T>}, and {@code Optional<T>}.
 *
 * @param <V> the value type stored in this registry
 */
public class Registry<V> {

    /** The underlying key-to-value store. */
    private final Map<String, V> store = new HashMap<>();

    /** Tags applied to each key. */
    private final Map<String, Set<String>> tags = new HashMap<>();

    /**
     * Registers a value under the given key.
     *
     * @param key   the lookup key
     * @param value the value to register
     */
    public void register(String key, V value) {
        store.put(key, value);
    }

    /**
     * Returns the value for {@code key}, or empty if absent.
     *
     * @param key the lookup key
     * @return an Optional wrapping the value, or empty
     */
    public Optional<V> lookup(String key) {
        return Optional.ofNullable(store.get(key));
    }

    /**
     * Returns all registered values as a list.
     *
     * @return an unordered list of all values
     */
    public List<V> allValues() {
        return List.copyOf(store.values());
    }

    /**
     * Returns all keys in the registry.
     *
     * @return the set of all registered keys
     */
    public Set<String> keys() {
        return Set.copyOf(store.keySet());
    }

    /**
     * Returns the full tag-to-value mapping for {@code key}.
     *
     * @param key the lookup key
     * @return a map of tag names to tag values, possibly empty
     */
    public Map<String, String> tagsFor(String key) {
        return Map.of();
    }

    /**
     * Applies multiple registrations at once.
     *
     * @param entries a map of key-value pairs to register
     */
    public void registerAll(Map<String, V> entries) {
        store.putAll(entries);
    }
}
