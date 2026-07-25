/** A documented point. */
typedef struct Point {
  /** horizontal coordinate */
  int x;
  /** vertical coordinate */
  int y;
} Point;

/** Numeric scalar alias. */
typedef double Scalar;

union Number {
  int i;
  double d;
};

const int answer = 42;
int mutable_counter = 0;

/** Scale a point by a scalar. */
Point scale_point(const Point *point, Scalar factor);

typedef int (*BinaryOp)(int lhs, int rhs);
