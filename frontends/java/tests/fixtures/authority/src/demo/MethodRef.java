package demo;

import java.util.function.Function;
import java.util.function.Supplier;

/** Method and constructor references must be calls, not dropped tokens. */
public final class MethodRef {
    private MethodRef() {}

    static String parse(String raw) {
        return raw;
    }

    static final class Gate {
        <T> Gate(T value) {}
    }

    interface MakeGate {
        Gate make(String raw);
    }

    String viaRef() {
        Function<String, String> bound = MethodRef::parse;
        Supplier<MethodRef> constructed = MethodRef::new;
        Function<String, Integer> foreign = String::length;
        Supplier<MethodRef> escaped = MethodRef::\u006eew;
        Function<String, Integer> escapedInvoke = String::\u006cength;
        Supplier<MethodRef> commented = MethodRef::/*c*/new;
        Supplier<MethodRef> spaced = MethodRef:: new;
        MakeGate gated = Gate::<String>new;
        return parse("x");
    }
}
