/// Computes a value.
template <typename T>
struct Service {
    virtual T run(T value) = 0;
};

/// Implements the service.
struct Worker final : Service<int> {
    int run(int value) override { return value; }
};
