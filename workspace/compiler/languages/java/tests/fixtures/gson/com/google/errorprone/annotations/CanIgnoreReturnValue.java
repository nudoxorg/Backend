package com.google.errorprone.annotations;

import java.lang.annotation.ElementType;
import java.lang.annotation.Retention;
import java.lang.annotation.RetentionPolicy;
import java.lang.annotation.Target;

/**
 * NOT part of the Gson fixture. This is a minimal, hand-written stand-in for
 * {@code com.google.errorprone.annotations.CanIgnoreReturnValue} from the
 * {@code com.google.errorprone:error_prone_annotations} artifact, which Gson
 * depends on optionally (it is not vendored here — see the sibling {@code
 * com/google/gson} tree, which is real, unmodified Gson 2.11.0 source). This
 * stub exists only so that real Gson source files compile and doc-process
 * without pulling in a real external Maven dependency; its members mirror
 * the real annotation's public shape exactly (no members — a marker).
 */
@Target({ElementType.METHOD, ElementType.CONSTRUCTOR, ElementType.TYPE, ElementType.PACKAGE})
@Retention(RetentionPolicy.CLASS)
public @interface CanIgnoreReturnValue {}
