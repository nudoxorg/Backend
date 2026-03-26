#include <stdio.h>

struct Point {
    int x;
    int y;
};

void print_point(struct Point p) {
    printf("Point(%d, %d)\n", p.x, p.y);
}

int main() {
    struct Point p = {1, 2};
    print_point(p);
    return 0;
}