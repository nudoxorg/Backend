typedef struct {
  int x, y;
} Point;

Point new_point(int x, int y) {
  Point point = {x, y};
  return point;
}

typedef Point P;
