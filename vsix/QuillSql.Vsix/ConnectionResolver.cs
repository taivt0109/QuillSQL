using System;
using System.Collections.Specialized;
using System.Data;
using Microsoft.Data.SqlClient;
using System.Linq;
using System.Reflection;
using Microsoft.VisualStudio;
using Microsoft.VisualStudio.Shell;
using Microsoft.VisualStudio.Shell.Interop;

namespace QuillSql.Vsix
{
    internal sealed class ConnectionTarget
    {
        public string Key { get; set; }
        public string ConnectionString { get; set; }
        public string Description { get; set; }

        /// <summary>
        /// For Microsoft Entra logins: gets an access token from the sign-in SSMS keeps for the
        /// window, renewing it when it has expired. Null for Windows and SQL Server logins.
        /// </summary>
        public Func<string> AccessToken { get; set; }

        /// <summary>
        /// The login may need someone to sign in (Microsoft Entra MFA): after a failure, do not try
        /// again on its own, so a sign-in prompt never comes back while typing.
        /// </summary>
        public bool Interactive { get; set; }

        /// <summary>The same server and login, in another database of the server.</summary>
        public ConnectionTarget ForDatabase(string database)
        {
            var builder = new SqlConnectionStringBuilder(ConnectionString) { InitialCatalog = database };
            var parts = (Key ?? "").Split('|');
            if (parts.Length > 1)
            {
                parts[1] = database;
            }
            var server = Description?.Split(new[] { " / " }, StringSplitOptions.None)[0];
            return new ConnectionTarget { Key = string.Join("|", parts), ConnectionString = builder.ConnectionString, Description = server + " / " + database, AccessToken = AccessToken, Interactive = Interactive };
        }

        /// <summary>False for logins QuillSQL cannot open a connection with.</summary>
        public bool CanQuery => ConnectionString != null;

        /// <summary>A new (closed) connection, with a fresh access token for Microsoft Entra logins.</summary>
        public SqlConnection CreateConnection()
        {
            var connection = new SqlConnection(ConnectionString);
            if (AccessToken != null)
            {
                try
                {
                    connection.AccessToken = AccessToken();
                }
                catch (Exception ex)
                {
                    connection.Dispose();
                    var cause = (ex as TargetInvocationException)?.InnerException ?? ex;
                    throw new InvalidOperationException("The Microsoft Entra sign-in of this query window could not be renewed (" + cause.Message
                        + "). Run a query in the window to sign in again.", cause);
                }
            }
            return connection;
        }
    }

    /// <summary>
    /// Finds the connection of the active query window. SSMS exposes it only through
    /// non-public members of the editor's query executor, so every step is defensive.
    /// </summary>
    internal static class ConnectionResolver
    {
        private const BindingFlags Any = BindingFlags.Instance | BindingFlags.Public | BindingFlags.NonPublic;
        private static bool _loggedShape;
        private static bool _loggedEntra;

        public static ConnectionTarget TryGetActive()
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            try
            {
                var monitor = Package.GetGlobalService(typeof(SVsShellMonitorSelection)) as IVsMonitorSelection;
                if (monitor == null
                    || monitor.GetCurrentElementValue((uint)VSConstants.VSSELELEMID.SEID_DocumentFrame, out var frameObject) != VSConstants.S_OK
                    || !(frameObject is IVsWindowFrame frame)
                    || frame.GetProperty((int)__VSFPROPID.VSFPROPID_DocView, out var docView) != VSConstants.S_OK)
                {
                    return null;
                }
                return ForDocView(docView, true);
            }
            catch (Exception ex)
            {
                Log.Error("resolving connection", ex);
                return null;
            }
        }

        /// <summary>The connection of one query window's document view, or null.</summary>
        public static ConnectionTarget ForDocView(object docView, bool logShape = false)
        {
            try
            {
                // SSMS 21: the query editor control exposes its connection directly.
                var info = First(Get(docView, "ConnectionInfoList"));
                var connection = (Get(docView, "Connection") ?? Get(docView, "m_connection")) as IDbConnection;
                if (info == null && connection == null)
                {
                    // Older SSMS: through the query executor's connection strategy.
                    var strategy = Get(Get(docView, "QueryExecutor"), "ConnectionStrategy");
                    info = Get(strategy, "_connectionInfo") ?? Get(strategy, "ConnectionInfo") ?? Get(strategy, "UIConnectionInfo");
                    connection = Get(strategy, "Connection") as IDbConnection;
                }
                if (info == null && connection == null)
                {
                    if (logShape && !_loggedShape)
                    {
                        _loggedShape = true;
                        Log.Write("no connection on " + Describe(docView));
                    }
                    return null;
                }
                return Build(info, connection, logShape);
            }
            catch (Exception ex)
            {
                Log.Error("resolving connection", ex);
                return null;
            }
        }

        private static ConnectionTarget Build(object info, IDbConnection connection, bool log)
        {
            var server = Get(info, "ServerName") as string;
            if (string.IsNullOrEmpty(server))
            {
                server = (connection as System.Data.Common.DbConnection)?.DataSource
                    ?? (connection == null ? null : new SqlConnectionStringBuilder(connection.ConnectionString).DataSource);
            }
            if (string.IsNullOrEmpty(server))
            {
                return null;
            }
            var options = Get(info, "AdvancedOptions") as NameValueCollection;
            var database = connection?.Database;
            if (string.IsNullOrEmpty(database))
            {
                database = options?["DATABASE"];
            }

            var builder = new SqlConnectionStringBuilder
            {
                DataSource = server,
                ApplicationName = "QuillSQL IntelliSense",
                ConnectTimeout = 15,
            };
            if (!string.IsNullOrEmpty(database))
            {
                builder.InitialCatalog = database;
            }
            var encryption = options?["ENCRYPT_CONNECTION"];
            if (string.Equals(encryption, "Strict", StringComparison.OrdinalIgnoreCase))
            {
                builder.Encrypt = SqlConnectionEncryptOption.Strict;
            }
            else if (bool.TryParse(encryption, out var encrypt))
            {
                builder.Encrypt = encrypt;
            }
            if (bool.TryParse(options?["TRUST_SERVER_CERTIFICATE"], out var trust))
            {
                builder.TrustServerCertificate = trust;
            }

            // SSMS: 0 = Windows, 1 = SQL Server; other values are Microsoft Entra methods.
            var authentication = info == null ? 0 : Convert.ToInt32(Get(info, "AuthenticationType") ?? 0);
            var user = Get(info, "UserName") as string;
            switch (authentication)
            {
                case 0:
                    builder.IntegratedSecurity = true;
                    break;
                case 1:
                    builder.UserID = user;
                    builder.Password = Get(info, "Password") as string ?? "";
                    break;
                default:
                    // Microsoft Entra: SSMS keeps a renewable token for the window; ask it for a fresh
                    // token whenever a connection is opened.
                    var token = Get(info, "RenewableToken");
                    var getToken = token?.GetType().GetInterfaces().FirstOrDefault(i => i.Name == "IRenewableToken")?.GetMethod("GetAccessToken");
                    if (getToken != null)
                    {
                        if (log && !_loggedEntra)
                        {
                            _loggedEntra = true;
                            Log.Write("Microsoft Entra login (authentication type " + authentication + "): using the sign-in of the query window");
                        }
                        var entraUser = Get(token, "UserId") as string ?? user ?? "";
                        return new ConnectionTarget
                        {
                            Key = string.Join("|", server, database ?? "", authentication, entraUser),
                            ConnectionString = builder.ConnectionString,
                            Description = server + " / " + (database ?? "(default)"),
                            AccessToken = () => (string)getToken.Invoke(token, null),
                        };
                    }
                    // No token kept for the window (Microsoft Entra MFA, Integrated, Default...): open the
                    // connection the way SSMS does, with the same method and user. SSMS's SqlClient is
                    // the one loaded here, so the sign-in it already made is reused without a prompt.
                    var method = EntraMethod(authentication);
                    if (method != null)
                    {
                        builder.Authentication = method.Value;
                        if (!string.IsNullOrEmpty(user))
                        {
                            builder.UserID = user;
                        }
                        var password = Get(info, "Password") as string;
                        if ((method == SqlAuthenticationMethod.ActiveDirectoryPassword || method == SqlAuthenticationMethod.ActiveDirectoryServicePrincipal
                            || method == SqlAuthenticationMethod.ActiveDirectoryManagedIdentity) && !string.IsNullOrEmpty(password))
                        {
                            builder.Password = password;
                        }
                        if (log && !_loggedEntra)
                        {
                            _loggedEntra = true;
                            Log.Write("Microsoft Entra login (authentication type " + authentication + "): connecting with " + method.Value + " as " + (user ?? "(default)"));
                        }
                        return new ConnectionTarget
                        {
                            Key = string.Join("|", server, database ?? "", authentication, user ?? ""),
                            ConnectionString = builder.ConnectionString,
                            Description = server + " / " + (database ?? "(default)"),
                            Interactive = method == SqlAuthenticationMethod.ActiveDirectoryInteractive,
                        };
                    }
                    if (log)
                    {
                        Log.Write("authentication type " + authentication + " is not supported for metadata; suggestions use the script only");
                    }
                    // The window is still connected; callers that only need its name get one.
                    return new ConnectionTarget
                    {
                        Key = "",
                        ConnectionString = null,
                        Description = server + " / " + (database ?? "(default)"),
                    };
            }

            return new ConnectionTarget
            {
                Key = string.Join("|", server, database ?? "", authentication, user ?? ""),
                ConnectionString = builder.ConnectionString,
                Description = server + " / " + (database ?? "(default)"),
            };
        }

        /// <summary>The SqlClient method SSMS uses for its authentication type; null when there is none to use.</summary>
        private static SqlAuthenticationMethod? EntraMethod(int authentication)
        {
            switch (authentication)
            {
                case 2: return SqlAuthenticationMethod.ActiveDirectoryPassword;
                case 3: return SqlAuthenticationMethod.ActiveDirectoryIntegrated;
                case 4:
                case 5: return SqlAuthenticationMethod.ActiveDirectoryInteractive;
                case 7: return SqlAuthenticationMethod.ActiveDirectoryManagedIdentity;
                case 8: return SqlAuthenticationMethod.ActiveDirectoryServicePrincipal;
                case 9: return SqlAuthenticationMethod.ActiveDirectoryDefault;
                // 6 is device code: every connection would wait for a code typed in a browser.
                default: return null;
            }
        }

        /// <summary>The first element of a sequence, or the value itself when it is not a sequence.</summary>
        private static object First(object value)
        {
            if (value is System.Collections.IEnumerable items && !(value is string))
            {
                foreach (var item in items)
                {
                    return item;
                }
                return null;
            }
            return value;
        }

        /// <summary>Property or field value by name, searching non-public members and base types.</summary>
        public static object Get(object target, string name)
        {
            for (var type = target?.GetType(); type != null; type = type.BaseType)
            {
                var property = type.GetProperty(name, Any | BindingFlags.DeclaredOnly);
                if (property != null && property.GetIndexParameters().Length == 0)
                {
                    return property.GetValue(target);
                }
                var field = type.GetField(name, Any | BindingFlags.DeclaredOnly);
                if (field != null)
                {
                    return field.GetValue(target);
                }
            }
            return null;
        }

        private static string Describe(object target)
        {
            if (target == null)
            {
                return "null";
            }
            var type = target.GetType();
            var members = type.GetProperties(Any).Select(p => p.Name).Concat(type.GetFields(Any).Select(f => f.Name));
            return type.FullName + " {" + string.Join(", ", members) + "}";
        }
    }
}
