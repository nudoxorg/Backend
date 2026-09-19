"""Old-oracle parity fixture for Python declarations and documentation."""

from typing import Protocol

class Service(Protocol):
    """Computes a value."""
    def run(self, value: str) -> str: ...

class Worker:
    pass

def execute(worker: Service) -> str:
    """Invokes the service."""
    return worker.run("")
