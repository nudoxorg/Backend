using System;
using System.Collections.Generic;
using System.ComponentModel;

namespace Fidelity;

public interface IProperties
{
    int Number { get; }
    event Action Happened;
}

/// <summary>Documented contract with a <see cref="Widget.Run"/>.</summary>
public interface IContract
{
    void Run(string value);
}

[Obsolete("legacy", true)]
public partial class Widget : IContract, IProperties
{
    public const int Answer = 42;
    public static volatile int Shared;
    public required string? Name { get; init; }
    public event Action? Changed;
    public int Number => Answer;
    public event Action Happened;

    public Widget() { Name = ""; }
    void IContract.Run(string value) => Changed?.Invoke();
    public static Widget Create() => new() { Name = "" };
    public int this[int index] => index;
    public static Widget operator +(Widget left, Widget right) => left;
    public static explicit operator int(Widget value) => Answer;
    public static implicit operator Widget(int value) => new() { Name = value.ToString() };
    public async System.Threading.Tasks.Task Async(int input, params int[] values) { await System.Threading.Tasks.Task.Yield(); }
    public void References(in int input, ref int output, out int result, ref readonly int view, int value = 3) { result = input; output = result; }
    public IEnumerable<int> Iterate() { yield return Answer; }
    public static void Call() { var x = Create(); x.ToString(); new Widget { Name = "" }; }
    public unsafe int* Pointer(int[] values, int[,] matrix, int[][] jagged) => null;
    public void Types((int A, string B)? tuple, int?[] nullable, Dictionary<string, List<int[]>> map, dynamic value, MissingType missing) { }
    public unsafe void FunctionPointers(delegate*<int, void> managed, delegate* unmanaged[Cdecl]<int, int> native) { }
    partial void Half(int value);
}

public partial class Widget
{
    partial void Half(int value) { Changed?.Invoke(); }
    int IProperties.Number => Answer;
}

public enum Color { Red = 1, Blue = 2 }
public record RecordClass(int Value);
public record struct RecordStruct(int Value);

public struct Pair<T> where T : class, new()
{
    public T Value;
}

public interface IVariance<out T, in U> { }
public class AllConstraints<T, U, V> where T : class, new() where U : unmanaged where V : allows ref struct { }
public class NotNullConstraint<T> where T : notnull { }
public delegate int Compute(in int value, ref readonly int other, params int[] rest);

