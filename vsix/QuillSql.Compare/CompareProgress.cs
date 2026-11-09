using System;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;
using System.Windows.Media.Animation;
using System.Windows.Threading;

namespace QuillSql.Compare
{
    /// <summary>
    /// What a running comparison is doing: a row for each side with its current step and how far
    /// it is, then the steps of both (mapping, comparing), and the time taken so far. Fed with the
    /// engine's progress lines (`side`, `stage`, `done`, `total`).
    /// </summary>
    internal sealed class CompareProgress : Border
    {
        private enum State { Waiting, Running, Done, Failed }

        /// <summary>One step: state icon, title, what it is about, its current text and a bar.</summary>
        private sealed class Step
        {
            public readonly TextBlock Icon = Ui.Icon(Glyph.Recent, 14, CompareTheme.Muted);
            public readonly TextBlock Title = Ui.Text("", bold: true);
            public readonly TextBlock About = Ui.Text("", brush: CompareTheme.Muted);
            public readonly TextBlock Text = Ui.Text("", 12, brush: CompareTheme.Muted);
            public readonly ProgressBar Bar = new ProgressBar { Minimum = 0, Maximum = 1, Height = 4 };
            public readonly Border Track = new Border { Height = 4, CornerRadius = new CornerRadius(2), VerticalAlignment = VerticalAlignment.Center, ClipToBounds = true };
            public readonly TextBlock Percent = Ui.Text("", 12, brush: CompareTheme.Muted);
            public State State;
        }

        private readonly Step _source = new Step(), _target = new Step(), _map = new Step(), _compare = new Step();
        private readonly TextBlock _heading = Ui.Text("", 14, bold: true);
        private readonly TextBlock _elapsed = Ui.Text("", 12, brush: CompareTheme.Muted);
        private readonly Button _close;
        private readonly DispatcherTimer _timer = new DispatcherTimer { Interval = TimeSpan.FromSeconds(1) };
        private DateTime _started;

        public CompareProgress()
        {
            CornerRadius = new CornerRadius(8);
            BorderThickness = new Thickness(1);
            Padding = new Thickness(16, 12, 12, 14);
            Margin = new Thickness(28, 4, 28, 10);
            SetResourceReference(BackgroundProperty, "Q.Layer");
            SetResourceReference(BorderBrushProperty, "Q.CardStroke");
            Visibility = Visibility.Collapsed;

            _close = Ui.IconButton(Glyph.Cancel, "Close", () => Visibility = Visibility.Collapsed);
            _close.Visibility = Visibility.Collapsed;
            _close.Height = 28;
            _close.Width = 32;
            var top = new DockPanel { Margin = new Thickness(0, 0, 0, 6) };
            var right = Ui.Row(_elapsed, _close);
            right.Margin = new Thickness(0);
            _elapsed.Margin = new Thickness(8, 0, 4, 0);
            DockPanel.SetDock(right, Dock.Right);
            top.Children.Add(right);
            top.Children.Add(_heading);

            var grid = new Grid();
            grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(28) });
            grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto, MinWidth = 70 });
            grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
            grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(180) });
            grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(48) });
            var row = 0;
            foreach (var step in new[] { _source, _target, _map, _compare })
            {
                if (step == _map)
                {
                    // The steps of both sides, under a thin line.
                    var line = new Border { Height = 1, Margin = new Thickness(0, 6, 0, 6) };
                    line.SetResourceReference(Border.BackgroundProperty, "Q.Divider");
                    grid.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
                    Place(grid, line, row++, 0, 5);
                }
                grid.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
                step.Track.SetResourceReference(Border.BackgroundProperty, "Q.Divider");
                step.Track.Child = step.Bar;
                step.Track.Margin = new Thickness(12, 0, 8, 0);
                step.Icon.RenderTransformOrigin = new Point(0.5, 0.5);
                step.Icon.RenderTransform = new RotateTransform();
                step.Icon.HorizontalAlignment = HorizontalAlignment.Left;
                step.Title.Margin = new Thickness(0, 0, 12, 0);
                step.About.TextTrimming = step.Text.TextTrimming = TextTrimming.CharacterEllipsis;
                step.Percent.HorizontalAlignment = HorizontalAlignment.Right;
                var texts = Ui.Column(step.About, step.Text);
                texts.VerticalAlignment = VerticalAlignment.Center;
                texts.Margin = new Thickness(0, 3, 0, 3);
                Place(grid, step.Icon, row, 0);
                Place(grid, step.Title, row, 1);
                Place(grid, texts, row, 2);
                Place(grid, step.Track, row, 3);
                Place(grid, step.Percent, row, 4);
                row++;
            }
            Child = Ui.Column(top, grid);
            _timer.Tick += (s, e) => ShowElapsed();
        }

        private static void Place(Grid grid, UIElement e, int row, int column, int span = 1)
        {
            Grid.SetRow(e, row);
            Grid.SetColumn(e, column);
            Grid.SetColumnSpan(e, span);
            grid.Children.Add(e);
        }

        /// <summary>Shows the panel for a new comparison of `source` with `target` (as the header names them).</summary>
        public void Start(bool data, string source, string target)
        {
            _heading.Text = data ? "Comparing data" : "Comparing schemas";
            _heading.ClearValue(TextBlock.ForegroundProperty);
            Reset(_source, "Source", source, "Waiting to start");
            Reset(_target, "Target", target, "Waiting to start");
            Reset(_map, "Mapping", null, data ? "Pair tables and columns" : "Pair objects by name and mapping");
            Reset(_compare, "Comparing", null, data ? "Compare the rows of each table" : "Compare the objects' definitions");
            _close.Visibility = Visibility.Collapsed;
            _started = DateTime.Now;
            ShowElapsed();
            _timer.Start();
            Visibility = Visibility.Visible;
        }

        private void Reset(Step step, string title, string about, string text)
        {
            step.Title.Text = title;
            step.About.Text = about ?? "";
            step.About.Visibility = about == null ? Visibility.Collapsed : Visibility.Visible;
            step.About.ToolTip = about;
            Set(step, State.Waiting, text, null, null);
        }

        /// <summary>A progress line of the comparison; lines without a stage are left to the status text.</summary>
        public void Update(JsonObject line)
        {
            var stage = line.Str("stage");
            if (stage == null) return;
            var text = line.Str("progress") ?? "";
            long? done = line.ContainsKey("done") ? line.Long("done") : (long?)null;
            long? total = line.ContainsKey("total") ? line.Long("total") : (long?)null;
            var side = line.Long("side");
            if (side == 1 || side == 2)
            {
                var step = side == 1 ? _source : _target;
                if (stage == "failed") Set(step, State.Failed, text, null, null);
                else if (stage == "done") Set(step, State.Done, text, null, null);
                else Set(step, State.Running, text, done, total);
                return;
            }
            if (stage == "map")
            {
                Set(_map, State.Running, text, done, total);
            }
            else if (stage == "compare" || stage == "data")
            {
                if (_map.State != State.Done) Set(_map, State.Done, "Paired", null, null);
                Set(_compare, State.Running, text, done, total);
            }
        }

        /// <summary>
        /// The comparison did not finish: a side that failed keeps its error on show, with a close
        /// button; otherwise the panel goes away.
        /// </summary>
        public void Fail()
        {
            _timer.Stop();
            if (_source.State != State.Failed && _target.State != State.Failed)
            {
                Hide();
                return;
            }
            foreach (var step in new[] { _source, _target, _map, _compare })
            {
                if (step.State == State.Running) Set(step, State.Waiting, "Not finished", null, null);
            }
            _heading.Text = "The comparison did not finish";
            _heading.Foreground = CompareTheme.High;
            _close.Visibility = Visibility.Visible;
        }

        public void Hide()
        {
            _timer.Stop();
            foreach (var step in new[] { _source, _target, _map, _compare }) Spin(step, false);
            Visibility = Visibility.Collapsed;
        }

        private void ShowElapsed()
        {
            var t = DateTime.Now - _started;
            _elapsed.Text = (int)t.TotalMinutes + ":" + t.Seconds.ToString("00");
        }

        private static void Set(Step step, State state, string text, long? done, long? total)
        {
            step.State = state;
            step.Text.Text = text;
            step.Text.TextWrapping = state == State.Failed ? TextWrapping.Wrap : TextWrapping.NoWrap;
            step.Text.ToolTip = string.IsNullOrEmpty(text) ? null : text;
            switch (state)
            {
                case State.Waiting:
                    step.Icon.Text = Glyph.Recent;
                    step.Icon.Foreground = CompareTheme.Muted;
                    step.Text.Foreground = CompareTheme.Muted;
                    break;
                case State.Running:
                    step.Icon.Text = Glyph.Refresh;
                    step.Icon.Foreground = CompareTheme.Accent;
                    step.Text.Foreground = CompareTheme.Foreground;
                    break;
                case State.Done:
                    step.Icon.Text = Glyph.Check;
                    step.Icon.Foreground = CompareTheme.Success;
                    step.Text.Foreground = CompareTheme.Muted;
                    break;
                case State.Failed:
                    step.Icon.Text = Glyph.Warning;
                    step.Icon.Foreground = CompareTheme.High;
                    step.Text.Foreground = CompareTheme.High;
                    break;
            }
            Spin(step, state == State.Running);
            // A bar while running: how far when the counts are known, else moving.
            var running = state == State.Running;
            step.Track.Visibility = running ? Visibility.Visible : Visibility.Hidden;
            var known = running && done.HasValue && total.HasValue && total.Value > 0;
            step.Bar.IsIndeterminate = running && !known;
            step.Bar.Value = known ? Math.Min(1.0, (double)done.Value / total.Value) : 0;
            step.Percent.Text = known ? (int)Math.Round(100.0 * done.Value / total.Value) + "%" : "";
            step.Percent.ToolTip = known ? done.Value.ToString("N0") + " of " + total.Value.ToString("N0") : null;
        }

        /// <summary>Turns a running step's icon.</summary>
        private static void Spin(Step step, bool on)
        {
            var rotate = (RotateTransform)step.Icon.RenderTransform;
            var spinning = step.Icon.Tag is bool b && b;
            if (on == spinning) return;
            step.Icon.Tag = on;
            if (on) rotate.BeginAnimation(RotateTransform.AngleProperty, new DoubleAnimation(0, 360, TimeSpan.FromSeconds(1.6)) { RepeatBehavior = RepeatBehavior.Forever });
            else rotate.BeginAnimation(RotateTransform.AngleProperty, null);
        }
    }
}
