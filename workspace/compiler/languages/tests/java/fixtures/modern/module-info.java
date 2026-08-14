/**
 * A small, real (compiled and doc-processed through the actual oracle) module
 * exercising JPMS directives, sealed interfaces, records, and enum bodies —
 * modern-Java (17-21) constructs the vendored Gson 2.11.0 fixture does not
 * use (Gson targets an older bytecode level and ships no records or sealed
 * types). This is a supplementary richness probe, not "the" third-party
 * library test; see `producer_tests.rs` for which test is which.
 */
module com.example.modern {
    requires java.logging;

    exports com.example.modern;
    exports com.example.modern.spi to com.example.consumer;

    uses com.example.modern.Shape;
    provides com.example.modern.Shape with com.example.modern.Square;
}
