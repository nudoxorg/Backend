import { Button } from "@/components/Button";
import { NavBar } from "@/components/NavBar";
import { Footer } from "@/components/Footer";

export default function Home() {
  return (
    <main>
      <NavBar />
      <Button label="Start" />
      <Footer />
    </main>
  );
}
