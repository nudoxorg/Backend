import clsx from "clsx";
import type { Product } from "../App";
import { Button } from "./Button";
import styles from "./ProductCard.module.css";

export function ProductCard({ product }: { product: Product }) {
  return (
    <article className={clsx(styles.card, product.featured && styles.featured)}>
      <h2 className={styles.title}>{product.title}</h2>
      <p className={styles.price}>${product.price}</p>
      <Button variant="primary">Add to cart</Button>
    </article>
  );
}
