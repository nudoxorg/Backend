package com.example.modern;

import java.lang.annotation.ElementType;
import java.lang.annotation.Retention;
import java.lang.annotation.RetentionPolicy;
import java.lang.annotation.Target;

/**
 * A custom annotation with a defaulted element, mirroring real annotation
 * types such as Gson's {@code @Since}.
 */
@Target(ElementType.METHOD)
@Retention(RetentionPolicy.RUNTIME)
public @interface Retention2 {
    /** The number of retries; defaults to 3. */
    int retries() default 3;
}
