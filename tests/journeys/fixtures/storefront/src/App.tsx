import { Header } from "./components/Header";
import { ProductGrid } from "./components/ProductGrid";
import { ThemeToggle } from "./components/ThemeToggle";

export interface Product {
  id: string;
  title: string;
  price: number;
  featured: boolean;
}

const catalog: Product[] = [
  { id: "kettle", title: "Enamel kettle", price: 48, featured: true },
  { id: "press", title: "French press", price: 32, featured: false },
  { id: "grinder", title: "Burr grinder", price: 89, featured: false },
];

export function App() {
  return (
    <div className="app-shell">
      <Header title="Storefront" />
      <main className="container">
        <ProductGrid products={catalog} />
      </main>
      <ThemeToggle />
    </div>
  );
}
