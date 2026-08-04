package com.example;

/**
 * Objects that can be serialized to and from bytes.
 */
public interface Serializable {

    /**
     * Serialize this object to a byte array.
     *
     * @return the byte representation
     */
    byte[] toBytes();

    /**
     * Deserialize from a byte array.
     *
     * @param data the bytes to parse
     * @return a new instance
     */
    Serializable fromBytes(byte[] data);
}
