namespace advanced {

/** Template storage wrapper. */
template <typename T>
class Holder {
public:
  T value;
};

/** Adapter with template-template, type, and value parameters. */
template <template <typename> class Box, typename T, int N>
class Adapter {
public:
  Box<T> box;

  Adapter(Box<T> initial) : box(initial) {}

  T get() const { return box.value; }
  int size() const { return N; }
};

/** Return the input unchanged. */
template <typename T>
T identity(T value) {
  return value;
}

template <typename T>
T choose(T lhs, T rhs) {
  return lhs;
}

template <typename T>
T choose(T value) {
  return value;
}

using IntHolder = Holder<int>;

} // namespace advanced
