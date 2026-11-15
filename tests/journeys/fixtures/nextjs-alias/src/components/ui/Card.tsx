import { Button } from "../Button";

export default function Card({ title }: { title: string }) {
  return (
    <section className="card">
      <h2>{title}</h2>
      <Button label="Open" />
    </section>
  );
}
