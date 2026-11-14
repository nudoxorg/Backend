import { Button as Action, Card } from "@/components";
import { Footer } from "@/components/Footer";

export default function Dashboard() {
  return (
    <main>
      <Card title="Usage" />
      <Action label="Refresh" />
      <Footer />
    </main>
  );
}
