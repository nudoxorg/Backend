struct Point {
  int x;
  int y;
};

void inc_point(struct Point p) {
  p.x += 1;
  p.y += 1;
}

int main() {
  struct Point p = {1, 2};
  inc_point(p);
  return 0;
}
