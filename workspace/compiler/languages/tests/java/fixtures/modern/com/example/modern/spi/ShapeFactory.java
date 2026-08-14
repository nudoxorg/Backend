package com.example.modern.spi;

import com.example.modern.Shape;

/** SPI-facing factory contract, in its own exported-to-a-consumer package. */
public interface ShapeFactory {
    /** @return a freshly created shape */
    Shape create();
}
