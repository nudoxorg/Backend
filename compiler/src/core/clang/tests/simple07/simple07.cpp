namespace coverage {

/** Runtime mode. */
enum class Mode {
  /** Waiting for work. */
  Idle,
  Busy,
};

static int internal_counter = 0;
const int max_items = 16;
int fixed_values[3] = {1, 2, 3};

using Callback = int (*)(int lhs, int rhs);

/** Exercise callable and reference type shapes. */
int apply(Callback callback, const int &bias, int &&scratch, ...);

/** Exercise pointer constness and nesting. */
int pointer_shapes(const int *readonly_ptr, int *mutable_ptr, int * const fixed_ptr, const int **nested_ptr);

class Convertible {
protected:
  int secret;

public:
  ~Convertible() {}
  operator int() const { return secret; }
};

} // namespace coverage
