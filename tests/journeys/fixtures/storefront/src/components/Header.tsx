import styles from "./Header.module.css";

export function Header({ title }: { title: string }) {
  return (
    <header className={styles.header}>
      <h1 className={styles.title}>{title}</h1>
      <nav className={styles.nav}>
        <a href="/">Shop</a>
        <a href="/about">About</a>
      </nav>
    </header>
  );
}
