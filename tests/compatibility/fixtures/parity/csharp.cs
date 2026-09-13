namespace Parity;

/// <summary>Computes a value.</summary>
public interface IService<T> { T Run(T value); }

/// <summary>Implements the service.</summary>
public sealed class Worker : IService<string> {
    public string Run(string value) => value;
}
