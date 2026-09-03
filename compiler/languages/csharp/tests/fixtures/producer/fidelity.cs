using System;
using System.Collections.Generic;

namespace Fidelity;

/// <summary>Documented contract with a <see cref="Widget.Run"/>.</summary>
public interface IContract
{
    void Run(string value);
}

[Obsolete("legacy", true)]
public sealed class Widget : IContract
{
    public const int Answer = 42;
    public static volatile int Shared;
    public required string Name { get; init; }
    public event Action? Changed;

    public Widget() { Name = ""; }
    void IContract.Run(string value) => Changed?.Invoke();
    public static Widget Create() => new() { Name = "" };
    public int this[int index] => index;
    public static Widget operator +(Widget left, Widget right) => left;
    public static explicit operator int(Widget value) => Answer;
}

public enum Color { Red = 1, Blue = 2 }
public record RecordClass(int Value);
public record struct RecordStruct(int Value);

public struct Pair<T> where T : class, new()
{
    public T Value;
}

public delegate int Compute(int value);
