namespace math {

/** Base class for values. */
class Base {
public:
  virtual int id() const { return 0; }
};

/** Generic fixed-size box. */
template <typename T, int N>
class Box : public Base {
public:
  T value;
  static int created;

  Box(T initial) : value(initial) {}

  /** Return the stored value. */
  T get() const { return value; }

  void reset(T next) { value = next; }
  void reset() { value = T{}; }

  int id() const override { return N; }

private:
  int hidden;
};

int add(int lhs, int rhs) { return lhs + rhs; }

} // namespace math
